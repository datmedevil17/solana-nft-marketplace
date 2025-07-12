#![allow(unexpected_cfgs)]
#![allow(deprecated)]
use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};
use mpl_token_metadata::accounts::Metadata;
use mpl_token_metadata::types::DataV2;

declare_id!("11111111111111111111111111111111");

#[program]
pub mod royalty {
    use super::*;

    pub fn initialize_royalty_config(
        ctx: Context<InitializeRoyaltyConfig>,
        max_royalty_basis_points: u16,
        platform_fee_basis_points: u16,
    ) -> Result<()> {
        let royalty_config = &mut ctx.accounts.royalty_config;
        royalty_config.authority = ctx.accounts.authority.key();
        royalty_config.max_royalty_basis_points = max_royalty_basis_points;
        royalty_config.platform_fee_basis_points = platform_fee_basis_points;
        royalty_config.total_fees_collected = 0;
        royalty_config.bump = ctx.bumps.royalty_config;
        
        Ok(())
    }

    pub fn distribute_payment<'info>(
        ctx: Context<'_, '_, '_, 'info, DistributePayment<'info>>,
        sale_price: u64,
    ) -> Result<()> {
        let royalty_config = &ctx.accounts.royalty_config;
        let metadata = &ctx.accounts.metadata;
        
        // Calculate platform fee
        let platform_fee = (sale_price as u128 * royalty_config.platform_fee_basis_points as u128 / 10000) as u64;
        
        // Get metadata and calculate creator royalties
        let metadata_account = metadata.to_account_info();
        let metadata_data = Metadata::try_from(&metadata_account)?;
        
        let mut total_royalty_fee = 0u64;
        let mut creator_fees = Vec::new();
        
        if let Some(creators) = &metadata_data.creators {
            let seller_fee_basis_points = metadata_data.seller_fee_basis_points;
            let total_royalty_amount = (sale_price as u128 * seller_fee_basis_points as u128 / 10000) as u64;
            
            for creator in creators {
                if creator.verified {
                    let creator_fee = (total_royalty_amount as u128 * creator.share as u128 / 100) as u64;
                    creator_fees.push((creator.address, creator_fee));
                    total_royalty_fee += creator_fee;
                }
            }
        }
        
        // Calculate seller amount (total - platform fee - royalty fees)
        let seller_amount = sale_price
            .checked_sub(platform_fee)
            .ok_or(ErrorCode::ArithmeticError)?
            .checked_sub(total_royalty_fee)
            .ok_or(ErrorCode::ArithmeticError)?;
        
        // Transfer platform fee to treasury
        if platform_fee > 0 {
            let transfer_to_treasury = Transfer {
                from: ctx.accounts.buyer_token_account.to_account_info(),
                to: ctx.accounts.platform_treasury.to_account_info(),
                authority: ctx.accounts.buyer.to_account_info(),
            };
            token::transfer(
                CpiContext::new(ctx.accounts.token_program.to_account_info(), transfer_to_treasury),
                platform_fee,
            )?;
        }
        
        // Transfer royalties to creators
        for (creator_address, creator_fee) in creator_fees {
            if creator_fee > 0 {
                // Find the creator's token account in remaining accounts
                let creator_token_account = ctx.remaining_accounts
                    .iter()
                    .find(|acc| acc.key() == creator_address)
                    .ok_or(ErrorCode::CreatorAccountNotFound)?;
                
                let transfer_to_creator = Transfer {
                    from: ctx.accounts.buyer_token_account.to_account_info(),
                    to: creator_token_account.clone(),
                    authority: ctx.accounts.buyer.to_account_info(),
                };
                token::transfer(
                    CpiContext::new(ctx.accounts.token_program.to_account_info(), transfer_to_creator),
                    creator_fee,
                )?;
            }
        }
        
        // Transfer remaining amount to seller
        if seller_amount > 0 {
            let transfer_to_seller = Transfer {
                from: ctx.accounts.buyer_token_account.to_account_info(),
                to: ctx.accounts.seller_token_account.to_account_info(),
                authority: ctx.accounts.buyer.to_account_info(),
            };
            token::transfer(
                CpiContext::new(ctx.accounts.token_program.to_account_info(), transfer_to_seller),
                seller_amount,
            )?;
        }
        
        // Update total fees collected
        let royalty_config = &mut ctx.accounts.royalty_config;
        royalty_config.total_fees_collected += platform_fee;
        
        emit!(PaymentDistributed {
            sale_price,
            platform_fee,
            total_royalty_fee,
            seller_amount,
            mint: ctx.accounts.mint.key(),
        });
        
        Ok(())
    }

    pub fn calculate_royalties(
        ctx: Context<CalculateRoyalties>,
        sale_price: u64,
    ) -> Result<RoyaltyBreakdown> {
        let royalty_config = &ctx.accounts.royalty_config;
        let metadata = &ctx.accounts.metadata;
        
        // Calculate platform fee
        let platform_fee = (sale_price as u128 * royalty_config.platform_fee_basis_points as u128 / 10000) as u64;
        
        // Get metadata and calculate creator royalties
        let metadata_account = metadata.to_account_info();
        let metadata_data = Metadata::try_from(&metadata_account)?;
        
        let mut total_royalty_fee = 0u64;
        let mut creator_breakdown = Vec::new();
        
        if let Some(creators) = &metadata_data.creators {
            let seller_fee_basis_points = metadata_data.seller_fee_basis_points;
            let total_royalty_amount = (sale_price as u128 * seller_fee_basis_points as u128 / 10000) as u64;
            
            for creator in creators {
                if creator.verified {
                    let creator_fee = (total_royalty_amount as u128 * creator.share as u128 / 100) as u64;
                    creator_breakdown.push(CreatorRoyalty {
                        address: creator.address,
                        share: creator.share,
                        amount: creator_fee,
                    });
                    total_royalty_fee += creator_fee;
                }
            }
        }
        
        // Calculate seller amount
        let seller_amount = sale_price
            .checked_sub(platform_fee)
            .ok_or(ErrorCode::ArithmeticError)?
            .checked_sub(total_royalty_fee)
            .ok_or(ErrorCode::ArithmeticError)?;
        
        Ok(RoyaltyBreakdown {
            sale_price,
            platform_fee,
            total_royalty_fee,
            seller_amount,
            creators: creator_breakdown,
        })
    }

    pub fn update_royalty_config(
        ctx: Context<UpdateRoyaltyConfig>,
        max_royalty_basis_points: Option<u16>,
        platform_fee_basis_points: Option<u16>,
    ) -> Result<()> {
        let royalty_config = &mut ctx.accounts.royalty_config;
        
        if let Some(max_royalty) = max_royalty_basis_points {
            require!(max_royalty <= 10000, ErrorCode::InvalidRoyaltyBasisPoints);
            royalty_config.max_royalty_basis_points = max_royalty;
        }
        
        if let Some(platform_fee) = platform_fee_basis_points {
            require!(platform_fee <= 1000, ErrorCode::InvalidPlatformFee); // Max 10%
            royalty_config.platform_fee_basis_points = platform_fee;
        }
        
        Ok(())
    }

    pub fn withdraw_platform_fees(
        ctx: Context<WithdrawPlatformFees>,
        amount: u64,
    ) -> Result<()> {
        let royalty_config = &ctx.accounts.royalty_config;
        
        // Transfer from platform treasury to authority
        let transfer_to_authority = Transfer {
            from: ctx.accounts.platform_treasury.to_account_info(),
            to: ctx.accounts.authority_token_account.to_account_info(),
            authority: ctx.accounts.platform_treasury.to_account_info(),
        };
        
        let seeds = &[
            b"platform_treasury".as_ref(),
            &[ctx.bumps.platform_treasury],
        ];
        let signer_seeds = &[&seeds[..]];
        
        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                transfer_to_authority,
                signer_seeds,
            ),
            amount,
        )?;
        
        Ok(())
    }
}

#[derive(Accounts)]
pub struct InitializeRoyaltyConfig<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + RoyaltyConfig::LEN,
        seeds = [b"royalty_config"],
        bump
    )]
    pub royalty_config: Account<'info, RoyaltyConfig>,
    
    #[account(mut)]
    pub authority: Signer<'info>,
    
    #[account(
        init,
        payer = authority,
        token::mint = mint,
        token::authority = royalty_config,
        seeds = [b"platform_treasury"],
        bump
    )]
    pub platform_treasury: Account<'info, TokenAccount>,
    
    pub mint: Account<'info, anchor_spl::token::Mint>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct DistributePayment<'info> {
    #[account(mut)]
    pub royalty_config: Account<'info, RoyaltyConfig>,
    
    #[account(mut)]
    pub buyer: Signer<'info>,
    
    #[account(mut)]
    pub buyer_token_account: Account<'info, TokenAccount>,
    
    #[account(mut)]
    pub seller_token_account: Account<'info, TokenAccount>,
    
    #[account(mut)]
    pub platform_treasury: Account<'info, TokenAccount>,
    
    pub mint: Account<'info, anchor_spl::token::Mint>,
    
    /// CHECK: This is validated by the Metaplex program
    pub metadata: AccountInfo<'info>,
    
    pub token_program: Program<'info, Token>,
    
    // Creator token accounts are passed as remaining_accounts
}

#[derive(Accounts)]
pub struct CalculateRoyalties<'info> {
    pub royalty_config: Account<'info, RoyaltyConfig>,
    
    /// CHECK: This is validated by the Metaplex program
    pub metadata: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct UpdateRoyaltyConfig<'info> {
    #[account(
        mut,
        has_one = authority,
        seeds = [b"royalty_config"],
        bump = royalty_config.bump
    )]
    pub royalty_config: Account<'info, RoyaltyConfig>,
    
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct WithdrawPlatformFees<'info> {
    #[account(
        has_one = authority,
        seeds = [b"royalty_config"],
        bump = royalty_config.bump
    )]
    pub royalty_config: Account<'info, RoyaltyConfig>,
    
    pub authority: Signer<'info>,
    
    #[account(mut)]
    pub authority_token_account: Account<'info, TokenAccount>,
    
    #[account(
        mut,
        seeds = [b"platform_treasury"],
        bump
    )]
    pub platform_treasury: Account<'info, TokenAccount>,
    
    pub token_program: Program<'info, Token>,
}

#[account]
pub struct RoyaltyConfig {
    pub authority: Pubkey,
    pub max_royalty_basis_points: u16,
    pub platform_fee_basis_points: u16,
    pub total_fees_collected: u64,
    pub bump: u8,
}

impl RoyaltyConfig {
    pub const LEN: usize = 32 + 2 + 2 + 8 + 1;
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct CreatorRoyalty {
    pub address: Pubkey,
    pub share: u8,
    pub amount: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct RoyaltyBreakdown {
    pub sale_price: u64,
    pub platform_fee: u64,
    pub total_royalty_fee: u64,
    pub seller_amount: u64,
    pub creators: Vec<CreatorRoyalty>,
}

#[event]
pub struct PaymentDistributed {
    pub sale_price: u64,
    pub platform_fee: u64,
    pub total_royalty_fee: u64,
    pub seller_amount: u64,
    pub mint: Pubkey,
}

#[error_code]
pub enum ErrorCode {
    #[msg("Arithmetic error occurred")]
    ArithmeticError,
    #[msg("Invalid royalty basis points")]
    InvalidRoyaltyBasisPoints,
    #[msg("Invalid platform fee")]
    InvalidPlatformFee,
    #[msg("Creator account not found")]
    CreatorAccountNotFound,
    #[msg("Metadata account is invalid")]
    InvalidMetadataAccount,
    #[msg("Insufficient funds for payment")]
    InsufficientFunds,
}